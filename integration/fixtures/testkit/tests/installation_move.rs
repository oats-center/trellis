//! A whole installation moves to another one with `trellis-server export`
//! and `trellis-server import`, across releases: an installation the
//! previous release runs is exported, and a fresh installation of the build
//! under test imported into needs no setup. Its administrator and users sign
//! in with their IDs, their roles hold, its app signs them in, its theme is
//! served, and its device and service reconnect with the keys they have, the
//! service with its settings and waiting job. An installation in use takes
//! the export too, keeping its own user and app where the export's are in
//! the way.
//!
//! The previous release has no exporter, so the build under test exports
//! the database the previous release made and still runs.
//!
//! Needs `TRELLIS_TEST_SERVER_BIN`, the build under test; the earlier release
//! is downloaded and checked against its published checksum.
use std::path::PathBuf;
use std::time::Duration;

use runtime_trellis::apps::runtime_trellis_account_caller::Request as AccountCaller;
use runtime_trellis::apps::runtime_trellis_moving_caller::Request as MovingCaller;
use runtime_trellis::participants::runtime_trellis_moving_service::{
    types::Value as Payload, Participant as Moving, Provider as MovingProvider,
};
use serde_json::{json, Value};
use trellis_rs::auth::{discover_native_oauth, NativeCredentialScope, NativeOAuthClient};
use trellis_rs::client::{DeviceConnectOptions, UserConnectOptions};
use trellis_rs::generated::{Client, ParticipantDescriptor};
use trellis_rs::jobs::JobProcessError;
use trellis_rs::service::ServiceConnectOptions;
use trellis_runtime_apis::apis::trellis_auth_v1;
use trellis_test_fixture::participants::trellis_test_fixture_unsupported_device::Participant as Device;
use trellis_testkit::{NativeSource, TestUserIdentity, TrellisTestRuntime};

const PASSPHRASE: &str = "the passphrase of a moving installation";
const ADMIN: (&str, &str) = ("mover", "mover-password-0123");

/// The build under test.
fn current() -> PathBuf {
    std::env::var_os("TRELLIS_TEST_SERVER_BIN")
        .expect("TRELLIS_TEST_SERVER_BIN names the build under test")
        .into()
}

/// An authenticated client of `runtime`'s administrator, signed in through
/// the CLI's device flow with `capabilities`.
async fn administrator(
    runtime: &mut TrellisTestRuntime,
    capabilities: &[&str],
) -> trellis_auth_v1::Client {
    let server = discover_native_oauth(runtime.trellis_url(), &runtime.trust_roots())
        .await
        .unwrap();
    let native = NativeOAuthClient::open(
        runtime
            .workdir()
            .join("move-admin")
            .join(ulid::Ulid::generate().to_string()),
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
    let device = native
        .start_device(&json!([{
            "type": "trellis_capabilities",
            "required": capabilities,
            "optional": ["trellis.auth.v1:account.read"]
        }]))
        .await
        .unwrap();
    runtime
        .approve_device_login("trellis.cli", device.user_code())
        .await
        .unwrap();
    trellis_auth_v1::Client::from_generated(
        Client::connect_user(UserConnectOptions::new(
            server.nats_servers,
            30_000,
            device.complete().await.unwrap(),
        ))
        .await
        .unwrap(),
    )
}

/// The principal a client of `auth` signed in as.
async fn principal(auth: &trellis_auth_v1::Client) -> String {
    auth.users_resolve(&wire(json!({})))
        .await
        .unwrap()
        .user
        .principal_id
        .to_string()
}

fn wire<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).unwrap()
}

/// The theme the Portal shows on `runtime`.
async fn theme(runtime: &TrellisTestRuntime) -> Value {
    runtime
        .https_client_builder()
        .build()
        .unwrap()
        .get(format!("{}/portal/theme", runtime.trellis_url()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// Whether `items` has an entry of `kind` for `id`.
fn listed(report: &Value, list: &str, kind: &str, id: &str) -> bool {
    report[list].as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| item["kind"] == kind && item["id"] == id)
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_installation_moves_to_another_and_to_a_newer_release_without_setup() {
    let current = current();
    let release = {
        let output = std::process::Command::new(&current)
            .arg("--version")
            .output()
            .unwrap();
        let text = String::from_utf8(output.stdout).unwrap();
        text.trim()
            .trim_start_matches("trellis-server ")
            .split('+')
            .next()
            .unwrap()
            .to_owned()
    };
    let mut old = TrellisTestRuntime::builder()
        .native(NativeSource::PreviousRelease(release))
        .admin_username(ADMIN.0)
        .admin_password(ADMIN.1)
        .start()
        .await
        .unwrap();

    // The old installation: a person with a password, an app, a theme, a
    // stricter password rule, a device with its own key, and a service with
    // settings and a job no worker has taken, whose API people reach through
    // a role and another app.
    let user: TestUserIdentity = old.create_user("Moving person").await.unwrap();
    let admin = administrator(
        &mut old,
        &[
            "trellis.auth.v1:users.manage",
            "trellis.auth.v1:clients.manage",
            "trellis.auth.v1:platform.manage",
        ],
    )
    .await;
    let old_admin = principal(&admin).await;
    admin
        .portal_themes_put(&wire(json!({
            "name": "Moved theme",
            "colors": {},
            "darkColors": {},
            "copy": { "signInTitle": "Welcome back" },
            "expectedRevision": "0",
            "idempotencyKey": ulid::Ulid::generate().to_string()
        })))
        .await
        .unwrap();
    let settings = admin
        .sign_in_settings_get(&wire(json!({})))
        .await
        .unwrap()
        .settings;
    let mut stricter = serde_json::to_value(&settings).unwrap();
    stricter["passwordMinLength"] = json!(11);
    stricter["expectedRevision"] = stricter["revision"].clone();
    stricter.as_object_mut().unwrap().remove("revision");
    stricter["idempotencyKey"] = json!(ulid::Ulid::generate().to_string());
    admin
        .sign_in_settings_update(&wire(stricter))
        .await
        .unwrap();
    let device_deployment = admin
        .deployments_create(&wire(json!({
            "title": "Moving device",
            "kind": "device",
            "participantId": Device::ID,
            "packageEvidence": Device::package_evidence(),
            "idempotencyKey": ulid::Ulid::generate().to_string()
        })))
        .await
        .unwrap()
        .deployment;
    let (device_seed, device_key) = trellis_rs::auth::generate_session_keypair();
    admin
        .devices_provision(&wire(json!({
            "deploymentId": device_deployment.deployment_id,
            "title": "Moving device",
            "identityPublicKey": device_key,
            "idempotencyKey": ulid::Ulid::generate().to_string()
        })))
        .await
        .unwrap();
    drop(admin);
    let app = old
        .register_native_application::<AccountCaller>("account")
        .await
        .unwrap();
    let service = old.register_service::<Moving>("moving").await.unwrap();
    old.set_config(
        &service,
        json!({ "label": "moved label" }),
        json!({ "apiKey": "moved-key-0123456789" }),
    )
    .await
    .unwrap();
    // People reach the service's API only through a role.
    let ping = runtime_trellis::apis::runtime_trellis_moving_v1::rpc::Ping::CALLER_CAPABILITIES[0];
    let admin = administrator(&mut old, &["trellis.auth.v1:roles.manage"]).await;
    let pingers = admin
        .roles_put(&wire(json!({
            "expectedRevision": "0",
            "idempotencyKey": ulid::Ulid::generate().to_string(),
            "title": "Pingers",
            "description": "Reach the moving service.",
            "capabilityIds": [ping],
            "assignToNewUsers": false
        })))
        .await
        .unwrap();
    admin
        .role_assignments_assign(&wire(json!({
            "principalId": user.principal_id(),
            "roleId": pingers.role.role_id,
            "expectedRevision": "0",
            "idempotencyKey": ulid::Ulid::generate().to_string()
        })))
        .await
        .unwrap();
    drop(admin);
    let pinger = old
        .register_native_application::<MovingCaller>("pinger")
        .await
        .unwrap();
    let mut running = Moving::connect(service.connect_options()).await.unwrap();
    let job = MovingProvider::new(&mut running)
        .jobs_client()
        .create_work(Payload {
            value: "waiting work".into(),
            extra: Default::default(),
        })
        .await
        .unwrap();
    let job_id = job.identity().id.clone();
    drop((job, running));
    let device_connects = |runtime: &TrellisTestRuntime| {
        let (url, roots, seed) = (
            runtime.trellis_url().to_owned(),
            runtime.trust_roots(),
            device_seed.clone(),
        );
        async move {
            let server = discover_native_oauth(&url, &roots).await.unwrap();
            Client::connect_device(DeviceConnectOptions::<Device>::new(
                &url,
                &seed,
                server.nats_servers,
                server.trust,
            ))
            .await
            .map(|_| ())
        }
    };
    device_connects(&old).await.unwrap();

    // The build under test exports the database the previous release runs.
    let file = old.workdir().join("installation.trellis-installation");
    let exported = old
        .export_installation(Some(&current), &file, PASSPHRASE)
        .await
        .unwrap();
    assert_eq!(
        exported["report"]["deployments"].as_array().unwrap().len(),
        2
    );

    // A fresh installation of the build under test, set up with nothing but
    // its own administrator, which the export's takes the place of.
    let mut new = TrellisTestRuntime::builder()
        .native(NativeSource::Path {
            server: current.clone(),
        })
        .admin_username(ADMIN.0)
        .admin_password(ADMIN.1)
        .start()
        .await
        .unwrap();
    let checked = new
        .import_installation(&file, PASSPHRASE, true, None)
        .await
        .unwrap();
    assert_eq!(checked["import"]["applied"], false);
    assert!(listed(
        &checked["import"],
        "created",
        "user",
        user.principal_id()
    ));
    let imported = new
        .import_installation(&file, PASSPHRASE, false, Some(&old))
        .await
        .unwrap();
    let report = &imported["import"];
    assert_eq!(report["target"]["empty"], true, "{report}");
    assert_eq!(report["access"]["skipped"], json!([]), "{report}");
    for kind in ["user", "oauthClient", "portalTheme", "deployment"] {
        assert!(
            report["created"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["kind"] == kind),
            "{kind}: {report}"
        );
    }
    assert!(
        report["deployments"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["error"].is_null()),
        "{report}"
    );

    // Its administrator signs in with the same ID, as the runtime itself did
    // when it started again.
    let admin = administrator(&mut new, &["trellis.auth.v1:platform.manage"]).await;
    assert_eq!(principal(&admin).await, old_admin);
    let settings = admin
        .sign_in_settings_get(&wire(json!({})))
        .await
        .unwrap()
        .settings;
    assert_eq!(
        serde_json::to_value(&settings).unwrap()["passwordMinLength"],
        11
    );
    drop(admin);

    // The person signs in through the app with their own password, under
    // the same ID.
    let login = new.login_native_application_as(&app, &user).await.unwrap();
    let auth = trellis_auth_v1::Client::from_generated(
        Client::connect_user(login.connect_options()).await.unwrap(),
    );
    assert_eq!(principal(&auth).await, user.principal_id());
    drop(auth);

    // The theme is served.
    assert_eq!(theme(&new).await["name"], "Moved theme");

    // The device connects with its key, without activating again.
    device_connects(&new).await.unwrap();

    // The service connects with its seed, finds its settings, and its
    // waiting job runs here.
    let server = discover_native_oauth(new.trellis_url(), &new.trust_roots())
        .await
        .unwrap();
    let mut moved = tokio::time::timeout(
        Duration::from_secs(60),
        Moving::connect(ServiceConnectOptions::new(
            server.nats_servers,
            server.trust,
            service.seed(),
        )),
    )
    .await
    .expect("the moved service connects with its settings in place")
    .unwrap();
    let mut provider = MovingProvider::new(&mut moved);
    assert_eq!(provider.config().current().unwrap().label, "moved label");
    let (ran, mut runs) = tokio::sync::mpsc::unbounded_channel();
    provider
        .register_work(move |job| {
            let ran = ran.clone();
            async move {
                let _ = ran.send(job.id().to_owned());
                Ok::<_, JobProcessError<String>>(job.payload().clone())
            }
        })
        .await
        .unwrap();
    provider
        .runtime_trellis_moving_v1()
        .register_ping(|_, input| async move { Ok(input) });
    let lifecycle = moved.lifecycle();
    let serving = tokio::spawn(moved.run());
    assert!(lifecycle.ready().await, "the moved service serves");
    let ran = tokio::time::timeout(Duration::from_secs(30), runs.recv())
        .await
        .expect("the waiting job runs in the new installation")
        .unwrap();
    assert_eq!(ran, job_id);

    // The person reaches the service's API through the moved app, which
    // only the moved role lets them.
    let login = new
        .login_native_application_as(&pinger, &user)
        .await
        .unwrap();
    let moving = runtime_trellis::apis::runtime_trellis_moving_v1::Client::from_generated(
        Client::connect_user(login.connect_options()).await.unwrap(),
    );
    let pong = moving
        .ping(&Payload {
            value: "ping".into(),
            extra: Default::default(),
        })
        .await
        .unwrap();
    assert_eq!(pong.value, "ping");
    drop(moving);
    lifecycle.stop();
    serving.await.unwrap().unwrap();

    // An installation in use, whose own user has the person's username and
    // whose own app has the app's client ID, takes the rest of the export
    // and keeps those two as they are.
    let mut busy = TrellisTestRuntime::builder()
        .native(NativeSource::Path { server: current })
        .start()
        .await
        .unwrap();
    let own = busy
        .create_user_as("Someone else", user.username(), None)
        .await
        .unwrap();
    let admin = administrator(&mut busy, &["trellis.auth.v1:clients.manage"]).await;
    let own_admin = principal(&admin).await;
    admin
        .o_auth_clients_put(&wire(json!({
            "clientId": app.client_id(),
            "kind": "native",
            "title": "Another app",
            "description": "",
            "redirectUris": [],
            "development": false,
            "impliedConsent": [],
            "expectedRevision": "0",
            "idempotencyKey": ulid::Ulid::generate().to_string()
        })))
        .await
        .unwrap();
    drop(admin);
    let imported = busy
        .import_installation(&file, PASSPHRASE, false, None)
        .await
        .unwrap();
    let report = &imported["import"];
    assert_eq!(report["target"]["empty"], false, "{report}");
    assert!(
        listed(report, "skipped", "user", user.principal_id()),
        "{report}"
    );
    assert!(
        listed(report, "skipped", "oauthClient", app.client_id()),
        "{report}"
    );
    assert!(
        listed(report, "skipped", "defaultAdministrator", &old_admin),
        "{report}"
    );
    assert!(
        listed(report, "created", "portalTheme", "the installation's"),
        "{report}"
    );
    assert!(
        listed(
            report,
            "created",
            "deployment",
            &device_deployment.deployment_id.to_string()
        ),
        "{report}"
    );
    assert!(
        listed(report, "created", "deployment", service.deployment_id()),
        "{report}"
    );
    let admin = administrator(&mut busy, &["trellis.auth.v1:accounts.manage"]).await;
    assert_eq!(principal(&admin).await, own_admin);
    let found = admin
        .users_find(&wire(json!({ "userId": own.principal_id() })))
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(found).unwrap()["found"]["user"]["displayName"],
        "Someone else"
    );
    drop(admin);
    assert_eq!(theme(&busy).await["name"], "Moved theme");

    busy.shutdown().await.unwrap();
    new.shutdown().await.unwrap();
    old.shutdown().await.unwrap();
}
