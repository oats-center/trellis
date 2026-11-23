//! An installation made by the previous release, started by the build under
//! test on the same state directory and NATS store. The previous release is
//! the newest published one no newer than the build: the release it upgrades.
//!
//! Once both are stable releases, the upgrade must succeed and everything the
//! earlier release stored must still work. While either is a release
//! candidate, the build may instead refuse the installation, naming both
//! releases and changing nothing, and the earlier release must then still
//! serve it. The phase follows the two versions, so the test holds the stable
//! rule from the first stable release on without being changed.
//!
//! Needs `TRELLIS_TEST_SERVER_BIN`, the build under test; the earlier release
//! is downloaded and checked against its published checksum.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use futures_util::StreamExt as _;
use provider::types::ResourceValue;
use runtime_trellis::apis::runtime_trellis_runtime_v1::{events::Changed, Client as RuntimeClient};
use runtime_trellis::apps::runtime_trellis_account_caller::Request as AccountCaller;
use runtime_trellis::apps::runtime_trellis_jobs_admin::Request as JobsAdmin;
use runtime_trellis::participants::{
    runtime_trellis_provider as provider, runtime_trellis_webhook_service as webhooks,
};
use runtime_trellis::types::Value;
use serde_json::json;
use trellis_rs::auth::{discover_native_oauth, NativeCredentialScope, NativeOAuthClient};
use trellis_rs::client::UserConnectOptions;
use trellis_rs::jobs::{JobProcessError, JobState};
use trellis_rs::service::{ConnectedServiceRuntime, ServiceEventListenOptions};
use trellis_runtime_apis::apis::{trellis_auth_v1, trellis_jobs_v1};
use trellis_testkit::{
    NativeSource, TestRegisteredNativeClient, TestServiceIdentity, TestUserIdentity,
    TrellisTestRuntime,
};

const HOOK_TOKEN: &str = "upgrade-hook-token";
const PARTNER_PASSWORD: &str = "upgrade-partner-password";

/// What [`populate`] stored, for [`verify`] to find again.
struct Installation {
    user: TestUserIdentity,
    app: TestRegisteredNativeClient<AccountCaller>,
    provider: TestServiceIdentity,
    webhooks: TestServiceIdentity,
    object: Vec<u8>,
    job_id: String,
}

/// The build under test.
fn current() -> PathBuf {
    std::env::var_os("TRELLIS_TEST_SERVER_BIN")
        .expect("TRELLIS_TEST_SERVER_BIN names the build under test")
        .into()
}

/// An authenticated client of the sandbox administrator, signed in through
/// the CLI's device flow with `capabilities`.
async fn administrator(
    runtime: &mut TrellisTestRuntime,
    capabilities: &[&str],
) -> trellis_rs::generated::Client {
    let server = discover_native_oauth(runtime.trellis_url(), &runtime.trust_roots())
        .await
        .unwrap();
    let native = NativeOAuthClient::open(
        runtime
            .workdir()
            .join("upgrade-admin")
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
            "optional": []
        }]))
        .await
        .unwrap();
    runtime
        .approve_device_login("trellis.cli", device.user_code())
        .await
        .unwrap();
    trellis_rs::generated::Client::connect_user(UserConnectOptions::new(
        server.nats_servers,
        5_000,
        device.complete().await.unwrap(),
    ))
    .await
    .unwrap()
}

/// The principal `user` signs in as through the account app, which needs
/// the capability only the user's role grants.
async fn sign_in(runtime: &TrellisTestRuntime, installation: &Installation) -> String {
    let login = runtime
        .login_native_application_as(&installation.app, &installation.user)
        .await
        .unwrap();
    let auth = trellis_auth_v1::Client::from_generated(
        trellis_rs::generated::Client::connect_user(login.connect_options())
            .await
            .unwrap(),
    );
    let me = auth
        .sessions_me(&serde_json::from_value(json!({})).unwrap())
        .await
        .unwrap();
    serde_json::to_value(me).unwrap()["principal"]["principalId"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// The next event `events` delivers.
async fn next<T>(events: &mut tokio::sync::mpsc::UnboundedReceiver<T>) -> T {
    tokio::time::timeout(Duration::from_secs(20), events.recv())
        .await
        .expect("the event is delivered")
        .expect("the listener stays open")
}

/// Sends a webhook request with the endpoint's token secret and returns the
/// body the service received.
async fn deliver_webhook(
    runtime: &TrellisTestRuntime,
    identity: &TestServiceIdentity,
    service: &mut ConnectedServiceRuntime<webhooks::Participant>,
    body: &str,
) -> Vec<u8> {
    let mut deliveries = webhooks::Provider::new(service)
        .token()
        .unwrap()
        .subscribe()
        .await
        .unwrap();
    let urls = runtime.webhook_urls(identity).await.unwrap();
    let response = runtime
        .https_client_builder()
        .build()
        .unwrap()
        .post(&urls["token"])
        .header("x-hook-token", HOOK_TOKEN)
        .body(body.to_owned())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(20), deliveries.next())
        .await
        .expect("the accepted request is delivered")
        .expect("the delivery stream stays open")
        .unwrap()
        .body
        .0
}

/// Stores, through public APIs, one of each kind of thing an installation
/// keeps: a user whose role grants a capability, a service with settings, a
/// secret and a webhook endpoint, KV and store data, a finished job, and an
/// event published while its durable consumer was away.
async fn populate(runtime: &mut TrellisTestRuntime) -> Installation {
    // A user who can sign in to the account app only through a role.
    let user = runtime.create_user("Upgrade user").await.unwrap();
    let admin = trellis_auth_v1::Client::from_generated(
        administrator(
            runtime,
            &[
                "trellis.auth.v1:roles.manage",
                "trellis.auth.v1:users.manage",
            ],
        )
        .await,
    );
    let role = admin
        .roles_put(
            &serde_json::from_value(json!({
                "expectedRevision": "0",
                "idempotencyKey": ulid::Ulid::generate().to_string(),
                "title": "Account readers",
                "description": "Read their own account.",
                "capabilityIds": ["trellis.auth.v1:account.read"],
                "assignToNewUsers": false
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    admin
        .role_assignments_assign(
            &serde_json::from_value(json!({
                "principalId": user.principal_id(),
                "roleId": role.role.role_id,
                "expectedRevision": "0",
                "idempotencyKey": ulid::Ulid::generate().to_string()
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    drop(admin);
    let app = runtime
        .register_native_application::<AccountCaller>("account")
        .await
        .unwrap();

    // A service with a setting, a secret and a webhook endpoint that checks
    // the secret, which receives a request.
    let webhook_identity = runtime
        .register_service::<webhooks::Participant>("hooks")
        .await
        .unwrap();
    runtime
        .set_config(
            &webhook_identity,
            json!({ "partnerUser": "partner", "relayRanges": ["127.0.0.0/8"] }),
            json!({
                "signing": "upgrade-signing",
                "hookToken": HOOK_TOKEN,
                "partnerPassword": PARTNER_PASSWORD,
            }),
        )
        .await
        .unwrap();
    let mut hooks = webhooks::Participant::connect(webhook_identity.connect_options())
        .await
        .unwrap();
    assert_eq!(
        deliver_webhook(runtime, &webhook_identity, &mut hooks, "before").await,
        b"before"
    );
    drop(hooks);

    // A service with KV and store data, a finished job, and a durable
    // consumer that took one event and missed the next.
    let identity = runtime
        .register_service::<provider::Participant>("provider")
        .await
        .unwrap();
    let mut service = provider::Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
    let listener = service
        .listen_event::<Changed, _, _>(
            move |event, _| {
                let _ = sent.send(event.value);
                async { Ok(()) }
            },
            ServiceEventListenOptions::default(),
        )
        .await
        .unwrap();
    let mut provider = provider::Provider::new(&mut service);
    provider
        .register_work(|job| async move { Ok::<_, JobProcessError<String>>(job.payload().clone()) })
        .await
        .unwrap();
    let jobs = provider.jobs_client();
    let client = provider.client();
    let records = client.records().await.unwrap();
    let files = client.files().await.unwrap();
    let events = RuntimeClient::from_generated(service.generated_client());
    let lifecycle = service.lifecycle();
    let run = tokio::spawn(service.run());
    assert!(lifecycle.ready().await, "the service starts serving");
    records
        .put(
            "upgrade.kept",
            &ResourceValue {
                value: "kept".into(),
                extra: Default::default(),
            },
        )
        .await
        .unwrap();
    let object: Vec<u8> = (0..200_000_u32)
        .map(|index| (index * 7 % 251) as u8)
        .collect();
    files.write("upgrade/object", object.clone()).await.unwrap();
    let job = jobs
        .create_work(Value {
            value: "finished".into(),
            extra: Default::default(),
        })
        .await
        .unwrap();
    let job_id = job.identity().id.clone();
    assert_eq!(job.wait().await.unwrap().state, JobState::Completed);
    let event = |value: &str| Value {
        value: value.into(),
        extra: Default::default(),
    };
    events.publish_changed(&event("taken")).await.unwrap();
    assert_eq!(next(&mut received).await, "taken");
    drop(listener);
    events.publish_changed(&event("missed")).await.unwrap();
    drop((records, files, jobs, client, events));
    lifecycle.stop();
    run.await.unwrap().unwrap();

    let installation = Installation {
        user,
        app,
        provider: identity,
        webhooks: webhook_identity,
        object,
        job_id,
    };
    assert_eq!(
        sign_in(runtime, &installation).await,
        installation.user.principal_id()
    );
    installation
}

/// Checks that everything [`populate`] stored still works on the server
/// `runtime` runs now.
async fn verify(runtime: &mut TrellisTestRuntime, installation: &Installation) {
    // The user signs in with the same password and holds the role's
    // capability; the administrator signed in again when the server started.
    assert_eq!(
        sign_in(runtime, installation).await,
        installation.user.principal_id()
    );

    // The service connects with its setting and secret, and its endpoint
    // still checks the token secret and delivers.
    let mut hooks = webhooks::Participant::connect(installation.webhooks.connect_options())
        .await
        .unwrap();
    let settings = webhooks::Provider::new(&mut hooks)
        .config()
        .current()
        .unwrap();
    assert_eq!(settings.partner_user, "partner");
    assert_eq!(settings.partner_password, PARTNER_PASSWORD);
    assert_eq!(
        deliver_webhook(runtime, &installation.webhooks, &mut hooks, "after").await,
        b"after"
    );
    drop(hooks);

    // The service reads its KV and store data back, its durable consumer
    // receives the event it missed and not the one it took, and new jobs run.
    let mut service = provider::Participant::connect(installation.provider.connect_options())
        .await
        .unwrap();
    let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
    let listener = service
        .listen_event::<Changed, _, _>(
            move |event, _| {
                let _ = sent.send(event.value);
                async { Ok(()) }
            },
            ServiceEventListenOptions::default(),
        )
        .await
        .unwrap();
    let mut provider = provider::Provider::new(&mut service);
    provider
        .register_work(|job| async move { Ok::<_, JobProcessError<String>>(job.payload().clone()) })
        .await
        .unwrap();
    let jobs = provider.jobs_client();
    let client = provider.client();
    let records = client.records().await.unwrap();
    let files = client.files().await.unwrap();
    let lifecycle = service.lifecycle();
    let run = tokio::spawn(service.run());
    assert!(lifecycle.ready().await, "the service starts serving");
    assert_eq!(
        records
            .get("upgrade.kept")
            .await
            .unwrap()
            .map(|kept| kept.value),
        Some("kept".to_owned())
    );
    let object = files
        .read("upgrade/object")
        .await
        .unwrap()
        .expect("the stored file");
    assert!(
        object.as_ref() == installation.object.as_slice(),
        "the stored file changed"
    );
    assert_eq!(next(&mut received).await, "missed");
    let job = jobs
        .create_work(Value {
            value: "after".into(),
            extra: Default::default(),
        })
        .await
        .unwrap();
    assert_eq!(job.wait().await.unwrap().state, JobState::Completed);

    // The job finished before is still listed, finished.
    let reader = runtime
        .register_client::<JobsAdmin>(&format!("jobs reader {}", ulid::Ulid::generate()))
        .await
        .unwrap();
    let reader = trellis_jobs_v1::Client::from_generated(
        trellis_rs::generated::Client::connect_user(reader.connect_options())
            .await
            .unwrap(),
    );
    let request = serde_json::from_value(json!({})).unwrap();
    let listed = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(listed) = reader.query(&request).await {
                let listed = serde_json::to_value(&listed).unwrap();
                if let Some(job) = listed["items"].as_array().and_then(|items| {
                    items
                        .iter()
                        .find(|job| job["id"] == installation.job_id.as_str())
                }) {
                    break job.clone();
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the finished job is listed");
    assert_eq!(listed["state"], "completed");
    assert!(
        tokio::time::timeout(Duration::from_millis(500), received.recv())
            .await
            .is_err(),
        "the consumer received an event it had already taken"
    );
    drop((records, files, jobs, client, listener, reader));
    lifecycle.stop();
    run.await.unwrap().unwrap();
}

/// Every table of the installation's databases with its rows, as text. It
/// reads copies, so reading changes nothing: SQLite moves a database's log
/// into it when the last connection closes, which changes its bytes but not
/// its contents.
fn databases(runtime: &TrellisTestRuntime) -> BTreeMap<String, Vec<String>> {
    let var = runtime.workdir().join("config/var");
    let copies = runtime
        .workdir()
        .join(format!("database-copies-{}", ulid::Ulid::generate()));
    std::fs::create_dir(&copies).unwrap();
    let mut tables = BTreeMap::new();
    for entry in std::fs::read_dir(&var).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.ends_with(".sqlite") {
            continue;
        }
        let copy = copies.join(&name);
        std::fs::copy(&path, &copy).unwrap();
        let log = var.join(format!("{name}-wal"));
        if log.exists() {
            std::fs::copy(&log, copies.join(format!("{name}-wal"))).unwrap();
        }
        let connection = rusqlite::Connection::open(&copy).unwrap();
        let mut schema = connection
            .prepare("SELECT name, sql FROM sqlite_schema WHERE type='table' ORDER BY name")
            .unwrap();
        let schema: Vec<(String, String)> = schema
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for (table, sql) in schema {
            let mut rows = connection
                .prepare(&format!("SELECT * FROM \"{table}\""))
                .unwrap();
            let columns = rows.column_count();
            let mut rows: Vec<String> = rows
                .query_map([], |row| {
                    (0..columns)
                        .map(|index| row.get::<_, rusqlite::types::Value>(index))
                        .collect::<Result<Vec<_>, _>>()
                        .map(|values| format!("{values:?}"))
                })
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            rows.sort();
            rows.insert(0, sql);
            tables.insert(format!("{name} {table}"), rows);
        }
    }
    std::fs::remove_dir_all(copies).unwrap();
    tables
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_build_upgrades_the_previous_release_or_refuses_it_only_between_release_candidates() {
    let current = current();
    let release = {
        let output = std::process::Command::new(&current)
            .arg("--version")
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", current.display());
        let text = String::from_utf8(output.stdout).unwrap();
        let version = text.trim().trim_start_matches("trellis-server ");
        semver::Version::parse(version.split('+').next().unwrap()).unwrap()
    };
    let mut runtime = TrellisTestRuntime::builder()
        .native(NativeSource::PreviousRelease(release.to_string()))
        .start()
        .await
        .unwrap();
    let previous = semver::Version::parse(runtime.server_version()).unwrap();
    assert!(previous <= release, "{previous} is newer than {release}");
    let installation = populate(&mut runtime).await;
    let candidates = !release.pre.is_empty() || !previous.pre.is_empty();
    eprintln!(
        "upgrading {previous} to {release} ({})",
        if candidates {
            "release candidates: refusal allowed"
        } else {
            "stable releases: the upgrade must succeed"
        }
    );

    // The operator stops the earlier release and starts the build in its place.
    runtime.stop_server().await.unwrap();
    let before = databases(&runtime);
    let log = runtime.workdir().join("logs/trellis-server.log");
    let logged = std::fs::read(&log).unwrap().len();
    let upgraded = runtime
        .restart_with_server(NativeSource::Path {
            server: current.clone(),
        })
        .await;
    if let Err(error) = upgraded {
        assert!(
            candidates,
            "Trellis {release} must upgrade an installation of Trellis {previous}: {error}"
        );
        // The refusal names both releases, and the installation is as the
        // earlier release left it, which still starts and serves it.
        let output = std::fs::read(&log).unwrap();
        let output = String::from_utf8_lossy(&output[logged..]);
        // The server wraps its error report to the terminal's width.
        let squashed = |text: &str| {
            text.chars()
                .filter(|c| !c.is_whitespace() && *c != '│')
                .collect::<String>()
        };
        // A changed schema baseline names the release that made the database;
        // an incompatible built-in API, the release that installed it.
        let reported = squashed(&output);
        assert!(
            reported.contains(&squashed(&format!("Trellis {release} can't upgrade")))
                && ["made by", "installed by"].iter().any(|by| {
                    reported.contains(&squashed(&format!("{by} Trellis {previous}")))
                }),
            "the refusal names Trellis {release} and Trellis {previous}:\n{output}"
        );
        let after = databases(&runtime);
        let changed: Vec<_> = before
            .keys()
            .chain(after.keys())
            .filter(|table| before.get(*table) != after.get(*table))
            .collect();
        assert!(changed.is_empty(), "the refused start changed {changed:?}");
        runtime
            .restart_with_server(NativeSource::PreviousRelease(release.to_string()))
            .await
            .unwrap();
        assert_eq!(runtime.server_version(), previous.to_string());
        eprintln!("Trellis {release} refused the installation; Trellis {previous} serves it again");
    } else {
        eprintln!("Trellis {release} upgraded the installation");
    }
    verify(&mut runtime, &installation).await;
    runtime.shutdown().await.unwrap();
}

/// The stable-phase check on the build under test alone: it restarts on an
/// installation it made itself, and everything it stored still works.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_build_restarts_on_its_own_installation_with_everything_working() {
    let current = current();
    let mut runtime = TrellisTestRuntime::builder()
        .native(NativeSource::Path {
            server: current.clone(),
        })
        .start()
        .await
        .unwrap();
    let installation = populate(&mut runtime).await;
    runtime
        .restart_with_server(NativeSource::Path { server: current })
        .await
        .unwrap();
    verify(&mut runtime, &installation).await;
    runtime.shutdown().await.unwrap();
}
