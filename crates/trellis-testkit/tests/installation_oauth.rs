//! Every OAuth flow Trellis serves works after an installation moves, for
//! the apps and people the export carried: a person who signs in only
//! through an OIDC provider (Dex) that the source had disabled moves with
//! that sign-in, the provider arrives disabled, and once it is enabled the
//! person signs in through it (with the moved client secret) and lands on
//! their own user ID with their role; the moved browser app signs people in
//! with the authorization code flow (PKCE, DPoP), renews with the refresh
//! token, and signs out by revoking it and ending the Portal login; the
//! moved native app signs people in with the device code flow, through Dex
//! and with a password, and renews with the refresh token. A person the
//! target already had under another ID, by the same OIDC sign-in, gets the
//! source's role. The moved people and that person find the browser app's
//! State they kept on the source, and are not asked to consent again.
//!
//! The browser half lives in `ts/browser/installation_oauth.browser_driver.ts`,
//! which runs Dex and Chromium; this test owns both installations and moves
//! one into the other when the driver says it is ready.
use std::io::{BufRead, Write};

use serde_json::{json, Value};
use trellis_rs::auth::{discover_native_oauth, NativeCredentialScope, NativeOAuthClient};
use trellis_rs::client::UserConnectOptions;
use trellis_rs::generated::Client;
use trellis_runtime_apis::apis::trellis_auth_v1;
use trellis_testkit::TrellisTestRuntime;

const PASSPHRASE: &str = "the passphrase of a moving installation";
const PROVIDER: &str = "Company";
const LOCAL: (&str, &str) = ("moving-local", "moving local password 0123");

fn wire<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).unwrap()
}

fn key() -> String {
    ulid::Ulid::generate().to_string()
}

/// A native OAuth client of `runtime` for `client_id`, with its trust and
/// NATS servers.
async fn native(
    runtime: &TrellisTestRuntime,
    client_id: &str,
) -> (
    NativeOAuthClient,
    trellis_rs::client::RuntimeAuthorizationTrust,
    Vec<String>,
) {
    let server = discover_native_oauth(runtime.trellis_url(), &runtime.trust_roots())
        .await
        .unwrap();
    let client = NativeOAuthClient::open(
        runtime.workdir().join("native").join(key()),
        NativeCredentialScope {
            issuer: server.issuer.clone(),
            instance_id: server.trust.instance_id.clone(),
            account: server.trust.account.clone(),
            client_id: client_id.into(),
            credential_handle: ulid::Ulid::generate(),
        },
        server.trust.clone(),
    )
    .await
    .unwrap();
    (client, server.trust, server.nats_servers)
}

/// `runtime`'s administrator, signed in through the CLI's device flow.
async fn administrator(runtime: &mut TrellisTestRuntime) -> trellis_auth_v1::Client {
    let (client, _, servers) = native(runtime, "trellis.cli").await;
    let device = client
        .start_device(&json!([{
            "type": "trellis_capabilities",
            "required": [
                "trellis.auth.v1:users.manage",
                "trellis.auth.v1:accounts.manage",
                "trellis.auth.v1:roles.manage",
                "trellis.auth.v1:clients.manage"
            ],
            "optional": []
        }]))
        .await
        .unwrap();
    runtime
        .approve_device_login("trellis.cli", device.user_code())
        .await
        .unwrap();
    trellis_auth_v1::Client::from_generated(
        Client::connect_user(UserConnectOptions::new(
            servers,
            30_000,
            device.complete().await.unwrap(),
        ))
        .await
        .unwrap(),
    )
}

/// Adds Dex as an OIDC provider of the installation behind `admin`, pinned
/// to Dex's CA, and returns its provider ID.
async fn add_provider(admin: &trellis_auth_v1::Client, dex: &Value) -> String {
    let provider = admin
        .oidc_providers_put(&wire(json!({
            "issuer": dex["issuer"],
            "clientId": dex["clientId"],
            "clientSecret": dex["clientSecret"],
            "certificateFingerprint": dex["fingerprint"],
            "title": PROVIDER,
            "scopes": ["openid", "email", "profile"],
            "roleClaim": "",
            "claimFreshnessSeconds": 3600,
            "expectedRevision": "0",
            "idempotencyKey": key()
        })))
        .await
        .unwrap();
    let provider = serde_json::to_value(provider).unwrap()["provider"].clone();
    if provider["enabled"] != true {
        admin
            .oidc_providers_enable(&wire(json!({
                "providerId": provider["providerId"],
                "expectedRevision": provider["revision"],
                "idempotencyKey": key()
            })))
            .await
            .unwrap();
    }
    provider["providerId"].as_str().unwrap().to_owned()
}

/// Creates a person who signs in with `subject` at `provider`, and returns
/// their user ID.
async fn create_oidc_user(
    admin: &trellis_auth_v1::Client,
    username: &str,
    provider: &str,
    subject: &Value,
) -> String {
    let created = admin
        .users_create(&wire(json!({
            "username": username,
            "displayName": username,
            "identities": [{ "providerId": provider, "subject": subject }],
            "idempotencyKey": key()
        })))
        .await
        .unwrap();
    created.user.principal_id.to_string()
}

/// The users with app sessions of `client_id` on the installation behind `admin`.
async fn session_users(admin: &trellis_auth_v1::Client, client_id: &str) -> Vec<String> {
    let page = admin
        .authorization_sessions_list(&wire(json!({ "clientId": client_id })))
        .await
        .unwrap();
    let mut users: Vec<String> = serde_json::to_value(page).unwrap()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|session| session["principalId"].as_str().unwrap().to_owned())
        .collect();
    users.sort();
    users.dedup();
    users
}

/// The roles `user` holds on the installation behind `admin`.
async fn roles_of(admin: &trellis_auth_v1::Client, user: &str) -> Vec<String> {
    let page = admin
        .role_assignments_list(&wire(json!({ "principalId": user })))
        .await
        .unwrap();
    serde_json::to_value(page).unwrap()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|assignment| assignment["roleId"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn oauth_and_oidc_sign_in_work_for_moved_apps_and_people() {
    // The installations have administrators of their own, so nothing here
    // depends on whether the export's administrator moves.
    let mut source = TrellisTestRuntime::builder()
        .admin_username("source-admin")
        .start()
        .await
        .unwrap();
    let mut target = TrellisTestRuntime::builder()
        .admin_username("target-admin")
        .start()
        .await
        .unwrap();
    let app = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let app_origin = format!("http://{}", app.local_addr().unwrap());
    drop(app);
    let source_admin = administrator(&mut source).await;
    let mut target_admin = Some(administrator(&mut target).await);

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut driver = std::process::Command::new("node")
        .current_dir(&root)
        .arg("ts/browser/installation_oauth.browser_driver.ts")
        .env("TRELLIS_SOURCE_URL", source.trellis_url())
        .env("TRELLIS_TARGET_URL", target.trellis_url())
        .env("TRELLIS_SOURCE_CA", &source.trust_roots()[0])
        .env("TRELLIS_TARGET_CA", &target.trust_roots()[0])
        .env("TRELLIS_APP_ORIGIN", &app_origin)
        .env("TRELLIS_DRIVER_DIRECTORY", source.workdir().join("driver"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut answer = driver.stdin.take().unwrap();
    let lines = std::io::BufReader::new(driver.stdout.take().unwrap()).lines();

    let mut dex = Value::Null;
    let (mut browser_client, mut native_client) = (String::new(), String::new());
    let (mut mover, mut twin, mut local) = (String::new(), String::new(), String::new());
    let (mut twin_here, mut source_provider, mut target_provider, mut role) =
        (String::new(), String::new(), String::new(), String::new());
    let mut device = None;
    let mut steps = Vec::new();
    for line in lines {
        let line = line.unwrap();
        println!("{line}");
        let Some((step, data)) = line
            .strip_prefix('@')
            .and_then(|message| message.split_once(' '))
        else {
            continue;
        };
        let data: Value = serde_json::from_str(data).unwrap();
        steps.push(step.to_owned());
        let reply = match step {
            // The source: Dex as its provider, a browser and a native app,
            // a local person and two Dex people, all with a role that gives
            // what the apps ask for.
            "dex" => {
                dex = data;
                source_provider = add_provider(&source_admin, &dex).await;
                mover = create_oidc_user(
                    &source_admin,
                    "mover",
                    &source_provider,
                    &dex["subjects"]["mover"],
                )
                .await;
                twin = create_oidc_user(
                    &source_admin,
                    "twin",
                    &source_provider,
                    &dex["subjects"]["twin"],
                )
                .await;
                local = source_admin
                    .users_create(&wire(json!({
                        "username": LOCAL.0,
                        "displayName": "Moving local",
                        "password": LOCAL.1,
                        "idempotencyKey": key()
                    })))
                    .await
                    .unwrap()
                    .user
                    .principal_id
                    .to_string();
                let created = source_admin
                    .roles_put(&wire(json!({
                        "expectedRevision": "0",
                        "idempotencyKey": key(),
                        "title": "Movers",
                        "description": "Use the moving apps.",
                        "capabilityIds": [
                            "trellis.auth.v1:account.read",
                            "trellis.state.v1:values.read",
                            "trellis.state.v1:values.write",
                            "trellis.state.v1:values.delete"
                        ],
                        "assignToNewUsers": false
                    })))
                    .await
                    .unwrap();
                role = created.role.role_id.to_string();
                for user in [&mover, &twin, &local] {
                    source_admin
                        .role_assignments_assign(&wire(json!({
                            "principalId": user,
                            "roleId": role,
                            "expectedRevision": "0",
                            "idempotencyKey": key()
                        })))
                        .await
                        .unwrap();
                }
                browser_client = key();
                native_client = key();
                for (client_id, kind, redirects) in [
                    (
                        &browser_client,
                        "browser",
                        json!([format!("{app_origin}/callback")]),
                    ),
                    (&native_client, "native", json!([])),
                ] {
                    source_admin
                        .o_auth_clients_put(&wire(json!({
                            "clientId": client_id,
                            "kind": kind,
                            "title": format!("Moving {kind} app"),
                            "description": "",
                            "redirectUris": redirects,
                            "development": true,
                            "impliedConsent": [],
                            "expectedRevision": "0",
                            "idempotencyKey": key()
                        })))
                        .await
                        .unwrap();
                }
                json!({
                    "provider": PROVIDER,
                    "browserClient": browser_client,
                    "local": { "username": LOCAL.0, "password": LOCAL.1 }
                })
            }
            // The target has the same provider under its own ID, and the
            // twin as a person of its own, under another user ID.
            "source-signed-in" => {
                let admin = target_admin.as_ref().unwrap();
                target_provider = add_provider(admin, &dex).await;
                twin_here = create_oidc_user(
                    admin,
                    "twin-here",
                    &target_provider,
                    &dex["subjects"]["twin"],
                )
                .await;
                assert_ne!(twin_here, twin);
                json!({})
            }
            // The target's copy of the provider then holds a secret Dex
            // refuses, so its people sign in after the move only with the
            // secret the export carried. The source disables Dex, and then
            // moves.
            "target-twin-signed-in" => {
                let admin = target_admin.take().unwrap();
                let current = admin
                    .oidc_providers_get(&wire(json!({ "providerId": target_provider })))
                    .await
                    .unwrap();
                let current = serde_json::to_value(current).unwrap()["provider"].clone();
                admin
                    .oidc_providers_put(&wire(json!({
                        "providerId": target_provider,
                        "issuer": current["issuer"],
                        "clientId": current["clientId"],
                        "clientSecret": "a secret Dex does not know",
                        "title": PROVIDER,
                        "scopes": current["scopes"],
                        "roleClaim": current["roleClaim"],
                        "claimFreshnessSeconds": current["claimFreshnessSeconds"],
                        "expectedRevision": current["revision"],
                        "idempotencyKey": key()
                    })))
                    .await
                    .unwrap();
                drop(admin);
                let current = source_admin
                    .oidc_providers_get(&wire(json!({ "providerId": source_provider })))
                    .await
                    .unwrap();
                source_admin
                    .oidc_providers_disable(&wire(json!({
                        "providerId": source_provider,
                        "expectedRevision":
                            serde_json::to_value(current).unwrap()["provider"]["revision"],
                        "idempotencyKey": key()
                    })))
                    .await
                    .unwrap();
                let file = source.workdir().join("installation.trellis-installation");
                source
                    .export_installation(None, &file, PASSPHRASE)
                    .await
                    .unwrap();
                let imported = target
                    .import_installation(&file, PASSPHRASE, false, Some(&source))
                    .await
                    .unwrap();
                let report = &imported["import"];
                for (kind, id) in [
                    ("user", &mover),
                    ("user", &local),
                    ("oauthClient", &browser_client),
                    ("oauthClient", &native_client),
                ] {
                    assert!(
                        report["created"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|item| item["kind"] == kind && item["id"] == id.as_str()),
                        "{kind} {id}: {report}"
                    );
                }
                // Dex arrived disabled, as the source had it, and the mover
                // with their only sign-in, through it. Enabling Dex lets them
                // sign in.
                let admin = administrator(&mut target).await;
                let current = admin
                    .oidc_providers_get(&wire(json!({ "providerId": target_provider })))
                    .await
                    .unwrap();
                let current = serde_json::to_value(current).unwrap()["provider"].clone();
                assert_eq!(current["enabled"], false, "{current}");
                let found = admin
                    .users_find(&wire(json!({ "userId": mover })))
                    .await
                    .unwrap();
                let identities =
                    serde_json::to_value(found).unwrap()["found"]["identities"].clone();
                assert!(
                    identities.as_array().unwrap().iter().any(|identity| {
                        identity["providerId"] == target_provider.as_str()
                            && identity["subject"] == dex["subjects"]["mover"]
                    }),
                    "{identities}"
                );
                admin
                    .oidc_providers_enable(&wire(json!({
                        "providerId": target_provider,
                        "expectedRevision": current["revision"],
                        "idempotencyKey": key()
                    })))
                    .await
                    .unwrap();
                target_admin = Some(admin);
                json!({})
            }
            // Each sign-in through the moved app lands on the person's
            // own user ID: the moved ones keep theirs, and the twin is the
            // target's own account.
            "target-app-signed-in" => {
                let who = match data["who"].as_str().unwrap() {
                    "mover" => &mover,
                    "local" => &local,
                    _ => &twin_here,
                };
                let users = session_users(target_admin.as_ref().unwrap(), &browser_client).await;
                assert!(users.contains(who), "{who} signed in: {users:?}");
                assert!(!users.contains(&twin), "{users:?}");
                json!({})
            }
            // Device code for the moved native app; the driver approves it.
            "device" => {
                let (client, trust, servers) = native(&target, &native_client).await;
                let started = client
                    .start_device(&json!([{
                        "type": "trellis_capabilities",
                        "required": ["trellis.auth.v1:account.read"],
                        "optional": []
                    }]))
                    .await
                    .unwrap();
                let reply = json!({
                    "verificationUri": started.verification_uri(),
                    "userCode": started.user_code()
                });
                device = Some((started, trust, servers));
                reply
            }
            // The approved device signs in as the person who approved it,
            // and renews with the refresh token.
            "device-approved" => {
                let (started, trust, servers) = device.take().unwrap();
                let mut session = started.complete().await.unwrap();
                session.refresh(trust).await.unwrap();
                let auth = trellis_auth_v1::Client::from_generated(
                    Client::connect_user(UserConnectOptions::new(servers, 30_000, session))
                        .await
                        .unwrap(),
                );
                let user = auth
                    .users_resolve(&wire(json!({})))
                    .await
                    .unwrap()
                    .user
                    .principal_id
                    .to_string();
                let expected = match data["who"].as_str().unwrap() {
                    "mover" => &mover,
                    _ => &local,
                };
                assert_eq!(&user, expected);
                json!({})
            }
            "done" => json!({}),
            other => panic!("the driver sent an unknown step {other}"),
        };
        writeln!(answer, "{reply}").unwrap();
    }
    let status = driver.wait().unwrap();
    assert!(
        status.success(),
        "the browser driver failed after {steps:?}"
    );
    assert_eq!(steps.last().map(String::as_str), Some("done"), "{steps:?}");

    // The twin the target already had now holds the source's role, and the
    // moved people theirs.
    let admin = target_admin.unwrap();
    for user in [&twin_here, &mover, &local] {
        assert!(roles_of(&admin, user).await.contains(&role), "{user}");
    }
    drop((admin, source_admin));
    target.shutdown().await.unwrap();
    source.shutdown().await.unwrap();
}
